; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @main() {
entry:
  %merge_1 = alloca i32
  %merge_3 = alloca i32
  %merge_6 = alloca i32
  %t0 = add i32 0, 0
  store i32 %t0, ptr %merge_1
  %t1 = add i32 0, 1
  store i32 %t1, ptr %merge_3
  %t2 = add i32 0, 0
  %t3 = add i32 0, 14
  store i32 %t2, ptr %merge_6
  br label %bb1
bb1:
  %t4 = load i32, ptr %merge_6
  %t5 = icmp sle i32 %t4, %t3
  br i1 %t5, label %bb2, label %bb4
bb2:
  %t6 = load i32, ptr %merge_1
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t6)
  %t8 = load i32, ptr %merge_1
  %t9 = load i32, ptr %merge_3
  %t10 = add i32 %t8, %t9
  %t11 = load i32, ptr %merge_3
  store i32 %t11, ptr %merge_1
  store i32 %t10, ptr %merge_3
  br label %bb3
bb3:
  %t12 = add i32 0, 1
  %t13 = load i32, ptr %merge_6
  %t14 = add i32 %t13, %t12
  store i32 %t14, ptr %merge_6
  br label %bb1
bb4:
  ret i32 0
}


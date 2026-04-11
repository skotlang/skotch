; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @main() {
entry:
  %merge_6 = alloca i32
  %t0 = add i32 0, 10
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t0)
  %t2 = add i32 0, 1
  %t3 = add i32 0, 3
  store i32 %t2, ptr %merge_6
  br label %bb1
bb1:
  %t4 = load i32, ptr %merge_6
  %t5 = icmp sle i32 %t4, %t3
  br i1 %t5, label %bb2, label %bb4
bb2:
  %t6 = load i32, ptr %merge_6
  %t7 = add i32 0, 100
  %t8 = mul i32 %t6, %t7
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t8)
  br label %bb3
bb3:
  %t10 = add i32 0, 1
  %t11 = load i32, ptr %merge_6
  %t12 = add i32 %t11, %t10
  store i32 %t12, ptr %merge_6
  br label %bb1
bb4:
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t0)
  ret i32 0
}


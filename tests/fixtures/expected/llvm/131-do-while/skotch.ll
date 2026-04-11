; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @main() {
entry:
  %merge_1 = alloca i32
  %t0 = add i32 0, 1
  store i32 %t0, ptr %merge_1
  br label %bb1
bb1:
  %t1 = load i32, ptr %merge_1
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t1)
  %t3 = load i32, ptr %merge_1
  %t4 = add i32 0, 2
  %t5 = mul i32 %t3, %t4
  store i32 %t5, ptr %merge_1
  br label %bb2
bb2:
  %t6 = load i32, ptr %merge_1
  %t7 = add i32 0, 16
  %t8 = icmp sle i32 %t6, %t7
  br i1 %t8, label %bb1, label %bb3
bb3:
  ret i32 0
}


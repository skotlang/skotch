; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @main() {
entry:
  %merge_1 = alloca i32
  %t0 = add i32 0, 0
  store i32 %t0, ptr %merge_1
  br label %bb1
bb1:
  %t1 = load i32, ptr %merge_1
  %t2 = add i32 0, 100
  %t3 = icmp slt i32 %t1, %t2
  br i1 %t3, label %bb2, label %bb3
bb2:
  %t4 = load i32, ptr %merge_1
  %t5 = add i32 0, 1
  %t6 = add i32 %t4, %t5
  store i32 %t6, ptr %merge_1
  %t7 = load i32, ptr %merge_1
  %t8 = load i32, ptr %merge_1
  %t9 = mul i32 %t7, %t8
  %t10 = add i32 0, 50
  %t11 = icmp sgt i32 %t9, %t10
  br i1 %t11, label %bb4, label %bb5
bb3:
  %t12 = load i32, ptr %merge_1
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t12)
  ret i32 0
bb4:
  br label %bb3
bb5:
  br label %bb6
bb6:
  br label %bb1
}

